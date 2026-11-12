# 0084. Gradle's test classes and their methods

- **Status:** accepted
- **Date:** 2026-11-12
- **Rules:** C-15, C-30, C-32, C-39, C-46; relies on C-31, C-34, C-38, ADR 0069, 0070, 0071,
  0077, 0085, RFC 0004

## Context

RFC 0004 §4.5 names Gradle as the runner for Java, Kotlin and Android's local unit tests, with a
test class as the test file and a method as the test. This decision is its adapter, the first of a
JVM build tool. A spike ran it on Gradle 8.14.5 and 9.3.1, Android's Gradle plugin 8.13.2 and
9.1.1, Kotlin 2.3, JUnit 4 and 5, TestNG, Spock and Kotest, and found where the RFC was too
hopeful: Gradle's own test filter can't leave out a nested class's method on Gradle 9 nor a TestNG
method at all, and its patterns cost seconds per thousand tests. Where they differ, this decision
and C-46 hold over §4.5.

## Decision

1. **Loading needs one line in the settings file**, which `hushrun init` proposes in its "Change
   by hand" list and never writes: `if (System.getenv("HUSHRUN_GRADLE")) apply from:
   System.getenv("HUSHRUN_GRADLE")` in `settings.gradle`, after any `pluginManagement {}` and
   `plugins {}` block (Groovy refuses anything before them), or `System.getenv("HUSHRUN_GRADLE")
   ?.let { apply(from = it) }` in `settings.gradle.kts`. No route from the environment alone loads
   code into a build: Gradle reads no init script from a variable, `ORG_GRADLE_PROJECT_*` only sets
   properties, and `JAVA_TOOL_OPTIONS` reaches the client and the test JVM, never a daemon already
   running. hushrun writes its Groovy script to `<cache dir>/adapters/gradle-<hash>/` and names it
   in `HUSHRUN_GRADLE`, and the test-JVM hook's jar (ADR 0085) in `HUSHRUN_GRADLE_HOOK`. Groovy,
   since `apply from:` takes it from either DSL with no Kotlin compile, and it reaches Android's and
   Kotlin's extensions without compiling against them. A settings file without the line, or none,
   loads nothing, and the run says so once with the line.
2. **The script reports facts and decides nothing.** For each `Test` task it writes one file in
   `HUSHRUN_UNITS`, a `hushrun.unit-facts/v1` file of no unit whose `settings.gradle` is the task's
   report: Gradle's own test events as they came (each invocation's class, name, parent suites and
   result), how the task ended, its test classes by their names, what it left out, and its
   classpath mapped to the build's projects. hushrun turns reports into each class's and test's
   facts (`units::gradle::observe`), where unit tests feed it reports the spike's builds wrote. A
   report of its own over hushrun's facts written in Groovy: the rules below are Rust, tested
   without Gradle, and the script stays a recorder.
3. **Every per-run path comes from `HUSHRUN_UNITS` when a task executes**: the plan
   (`hushrun.json`), the report, the hook's list and its events. Nothing of a run is read while the
   build is configured, so the configuration cache is reused from run to run with new plans, a
   daemon started without hushrun's variables picks them up, and a second run of an audit (another
   `HUSHRUN_UNITS`) never sees the first run's lists. The variables read while configuring are the
   script's and the hook's paths, the same from run to run. What the spike found by running it
   stays: the build service is registered at the end of configuration (it also hears `buildSrc`'s
   tasks, and its first event freezes its parameters), the hook joins the classpath then (a
   classpath changed when the task starts is ignored under the configuration cache), and Gradle
   9's `failOnNoDiscoveredTests` gets a lazy convention.
4. **Test tasks**: every `Test` task of every project, Android's `test<Variant>UnitTest` and Kotlin
   Multiplatform's `jvmTest` included. Not followed, each said in the facts: a test task of an
   included build (another build, whose settings have no line; known by its name), an
   `AbstractTestTask` that isn't a `Test` (Kotlin/Native, Kotlin/JS), a framework the script
   doesn't know. Not seen at all: `buildSrc`'s tests, a `JavaExec` that runs tests, Android's
   instrumented tests (no `Test` task; §6 of the RFC). With Isolated Projects on, the script says so in Gradle's output and
   observes nothing: its configuration crosses projects.
5. **Names** (C-30). A test class is named by its source file: its outer class's `SourceFile`
   attribute, then exactly one file of that name at its package's path under the project's test
   source directories (Java, Groovy and Kotlin source sets but `main`, Android's `test*`, Kotlin's
   `*Test`), else exactly one such file that declares its package (Kotlin's package needn't be its
   directory), else `<project directory>::<class>`. A nested class is its outer class's; two classes
   of one file are one unit. One function in the script names them, for the exclusion and the
   report alike. A unit of no file is made of no file of the scope; the store keeps its row only
   while it runs, so it is reused every other run at best.
6. **A test's id** is `<class as Gradle names it>.<method>`: the method is Gradle's name, or for an
   invocation under a method's own suite of the same class (parametrized, repeated, factory, a
   Spock unrolled feature), that suite's; then `(args)`, `[n]` and `[n](x)` go. Every invocation of
   a method is one test, as RFC 0004 §3.1 has it for JUnit. A nested class's method is
   `pkg.Outer$Inner.method`. The script splits an id at the first `.` after which the class's
   outer class is one of the unit's: the hook's entry is `pkg.Outer$Inner#method`, JUnit 4's
   patterns `pkg.Outer$Inner.method` and `pkg.Outer$Inner.method[*]` (Parameterized names its
   tests `method[0: x=1]`).
7. **Selection** (C-32). A unit `reuse` lists is left out whole by an exclusion added to the task,
   which Gradle evaluates while it fingerprints the task's class files, at execution: a set lookup
   of each class file's outer class. A test `reuseTests` lists goes through the hook for the JUnit
   Platform and TestNG, one list per task, its path given to the test JVM by a JVM-argument
   provider computed when the task starts and no input of the task, while what the list holds is
   an input (`hushrunLeftOut`), so another list makes the task run and the same list at another
   path keeps it up to date. JUnit 4 has no hook: Gradle's filter patterns, 250 tests a task at most
   (the spike measured 16 to 19 s for 5,000), past which they simply run. Kotest's tests have no
   method to leave out: they run. A task left with no test by hushrun doesn't fail: no source when
   every class is out, `failOnNoMatchingTests` off and Gradle 9's convention when tests are. The
   task's own `--tests` applies too; a class missing because of it hasn't gone away, and its row
   stays.
8. **Outcomes** (C-39). From Gradle's own test events, a root suite's end, a last action for a task
   that sent none, and the build service for the rest. A test failed when an invocation failed,
   and finished when its task's root suite ended; a test that ran though the plan listed it ran,
   never reused. `initializationError`, `classMethod`, `executionError` and a suite that failed
   with no failed test under it fail their class, and its tests that didn't finish record nothing.
   A test skipped in a class whose setup failed, or under TestNG in a class with a failed test
   (TestNG reports a failing `@BeforeClass` as a failed test named after the method and skips the
   rest), didn't finish: no test records a pass it didn't earn. A task Gradle didn't execute (up to
   date, from its build cache, skipped) records nothing: its report, kept in the task's outputs,
   names its classes and what they read and no event. One that ended before its root suite records
   no pass; a failure outside every class, none for the task. Kotlin Multiplatform's
   `ignoreFailures` changes nothing: facts come from the events. A class two tasks ran (Android's
   debug and release) finished where both ran it whole.
9. **A flaky test is a failed one.** The test-retry plugin's retry that passed is a retried
   invocation: the first try failed, so the test failed, as ADR 0069 has a test file that failed and
   then passed record its fail and run next time. Its pass doesn't hide the fail.
10. **What a class read** (C-30, C-31): every file below the directory of its project, of each
    project its test classpath is built from (an entry below a project's build directory; below an
    included build, its directory up to `build/`), and of their source directories outside them;
    and each other entry of the repository on its classpath, a file read (`libs/local.jar`) or a
    directory read whole. **Sources** are the `.java`, `.kt`, `.groovy` and `.scala` files below a
    directory some test class read, but below a `buildSrc` or a directory with a settings file of
    its own (a build included for its plugins); everything else is rest: build scripts,
    `gradle.properties`, version catalogs, `buildSrc`, resources, manifests, Android's `res/`, and
    the files of a project no test class reads, so a change to one runs everything. A plan has no
    Gradle to ask: its units are the map's list of test classes' files and its directories those
    the map's units read (C-30), else the newest hint's (C-38). Hints are addressed by every file of
    those kinds, as Vitest's are (ADR 0071), since a machine without a map can't tell the sources.
11. **`not observed`**: a framework the script doesn't know, its own error, a classpath entry of
    the repository that is no file of it (built output outside every project's build directory),
    and a project, source directory or included build outside the repository.
12. **Which COMMANDs**: `gradle`, `gradlew`, `gradle.bat` or `gradlew.bat`, by name or path, with
    any tasks; the settings file is the one `-c` names, else the first in `-p`'s directory or the
    current one or above it. Not offered, said once: `--continuous`. A COMMAND that hides Gradle
    (`make test`, a script) gets the default adapter, which does nothing for it, and the line in
    the settings file adds nothing without `HUSHRUN_GRADLE`.

## Measured

What the script adds to a build that leaves nothing out, with a warm daemon, on the spike's
500 test classes of 20 tests each, in milliseconds, the median of seven runs taken in turn with and
without it (configuration: `--dry-run`; the task: `--rerun`, which runs every test):

| | configuration | JUnit 5, 10,000 tests | JUnit 4, 10,000 tests |
|---|---|---|---|
| Gradle 8.14.5, without | 350 | 1,445 | 873 |
| Gradle 8.14.5, with | 363 | 1,549 | 915 |
| Gradle 9.3.1, without | 530 | 1,869 | 1,405 |
| Gradle 9.3.1, with | 491 | 2,498 | 1,296 |

So about 5 to 7% of the test task, nothing measurable in configuration. The Gradle 8.14.5 rows
were taken on a quiet machine; the Gradle 9.3.1 rows while other builds loaded it, so a difference
under half a second there is noise. Of the JUnit Platform's cost on Gradle 9.3.1, about 0.4 s came
from the hook's events, which this adapter doesn't read (with them sent nowhere: 2,103); on Gradle
8.14.5 they cost nothing measurable.

## Not built

- **`reads: test`**: a test is taken to have read what its class read (RFC 0004 §6).
- **Android's instrumented tests** (`connectedAndroidTest`): not a `Test` task, and need a device.
- **Isolated Projects**: nothing is observed. It would need the shared state in a build service and
  a configuration per project.
- **Kotest's tests one by one**, and a `JavaExec` or other task that runs tests.
- A unit of no file keeps no row while reused; Windows paths and Gradle before 8.14 weren't run.

## Consequences

- A Gradle repository observes and reuses test classes, and with `unit: test` their methods, after
  one reviewed line in its settings file.
- A test class reused whole names no tests: `units.tests` counts the tests of the classes that ran.
- hushrun's extra input and output make a Test task's cache key differ from a build without
  hushrun: Gradle's build cache shares nothing between the two.

## Alternatives considered

- **An init script from the environment**: Gradle reads none; `GRADLE_USER_HOME` pointed elsewhere
  would move the person's caches, daemons and credentials.
- **Gradle's filter for every single test**: no pattern leaves out a nested class's method on
  Gradle 9, a plain one leaves out nothing under TestNG, and `*`-prefixed ones cost minutes.
- **Facts written in Groovy**: every rule above would live in an untested script.
