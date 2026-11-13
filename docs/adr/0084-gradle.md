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
   invocation under a method's own suite of the same class (parametrized, repeated, factory, a Spock
   unrolled feature), that suite's; then `(args)`, `[n]`, and `[n]` followed by `(x)`, go. Every
   invocation of a method is one test, as RFC 0004 §3.1 has it for JUnit. A nested class's method is
   `pkg.Outer$Inner.method`. The script splits an id at the first `.` after which the class's outer
   class is one of the unit's: the hook's entry is `pkg.Outer$Inner#method`, JUnit 4's patterns
   `pkg.Outer$Inner.method` and `pkg.Outer$Inner.method[*]` (Parameterized names its tests
   `method[0: x=1]`).
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
   names its classes and what they read and no event. That holds for an audited class too: a task
   whose list is the one of its last execution is up to date, Gradle's check stands, and the audit
   of its classes waits for a run in which Gradle executes it. Nothing is recorded meanwhile. One that ended before its root suite records
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

## Amendment 1: a skip earns no pass where something failed (2026-11-12)

Item 8 let a skipped test finish unless its own class failed, or under TestNG held a failed
test. A reviewer showed TestNG skipping every test of the suite after another class's
`@BeforeSuite` failed: the skipped classes were recorded as passed and left out next time.

The decision: each skipped test, and its class, is told `skipped` (ADR 0077, Amendment 2), so a
run whose COMMAND failed records nothing of them. And a skipped test finishes only when nothing of
the run failed (a task, a test, a suite): else it didn't, and its class didn't run whole.

## Amendment 2: a test class reads every project its build names (2026-11-12)

Item 10 followed the test classpath only. A project the build only runs, an annotation processor
(`annotationProcessor`, `kapt`, `ksp`) or one on `compileOnly`, is on no test classpath; when it had
tests of its own its files were sources, so a change to it changed no key, and the tests of the code
it generates were reused. The decision: a test class also reads every project its project declares
in any configuration, and what those declare outside their own tests (a configuration whose name
starts `test`, `androidTest`, `kaptTest` or `kspTest`, or holds `UnitTest` or `AndroidTest`, but
`testFixtures*`), transitively, from the declared dependencies, never by resolving them.

## Amendment 3: a test JVM that died, and the hook's events (2026-11-12)

Item 8 took a task whose root suite ended to have run its tests whole. When a test JVM dies Gradle
calls the test in flight skipped, ends the root suite, says nothing of a class whose class teardown
never ended, and fails the task with `Process 'Gradle Test Executor N' finished with non-zero exit
value`. The class was recorded as passed. And the adapter kept the hook's events and never read
them.

The decision: the report says where the task's test JVMs wrote the hook's events in this run, and
the messages of the task's failure. A task ran nothing whole when it failed and no test or suite of
it did, when its failure says its test process ended badly, or when its events can't be read; a
class the events started and never ended didn't run whole, and none of its tests finished but
those that failed. JUnit 4 under Gradle's own runner and TestNG write no test events: there only
the task's failure tells.

## Amendment 4: the hook's list, matched exactly and held to (2026-11-12)

Item 6's hook entry compared a method up to its first `[`. Spock features, parametrized display
names and Kotest names can hold one: three features that differ after it were all left out by one
entry, the failing one too, and the check passed with no test run. And JUnit 4's patterns for a
class of the default package, `CalcTest.add`, start with an upper-case letter, which Gradle reads as
a simple class name: `p.CalcTest.add` was left out too.

The decision: the hook matches a method's whole name, and a Vintage test's `name[…]` only under the
JUnit Vintage engine (ADR 0085, Amendment 2); each test it leaves out is told with the entry that
listed it. The report holds the hook's list, and the adapter holds the events to it: a test left
out that no entry names exactly, or an entry that was neither left out nor seen run in a class that
ran, makes the run start COMMAND once more with nothing left out (`Read.left_out`, ADR 0077,
Amendment 4). A JUnit 4 class of the default package, or whose name starts upper-case, gets no
pattern: its listed tests run.

## Amendment 5: each start of Gradle keeps its own facts (2026-11-12)

A task's facts file was named by the task, so a `gradlew` script that starts Gradle again after a
failure overwrote the first start's report: a test that failed and then passed was recorded as
passed. The decision: the build service, one instance a build, keeps each task's report as its
start's own file, with `start` (ADR 0077, Amendment 1), and the adapter holds every start's word as
it holds two tasks': a test failed when any start failed it, and finished, and its class ran whole,
only when every start and task that ran it did. A retry that passed after a failure in the same
start (the test-retry plugin) stays a failure (item 9). `--continue` and `--rerun-tasks` change
nothing: each task executes once a start. The hook's events directory is the task's in the run, so
a second start's JVMs write beside the first's, and a class either left open didn't run whole.

Known, and left as they are: Gradle names a test `<class>.<method>` under a unit that is its source
file, and Maven `<method>` under a unit that is its class, so the same test has another id under
each adapter, and no result is shared between them. An audited class whose task is up to date
doesn't run in that run (item 8): the audit waits for a run in which Gradle executes the task.

## Amendment 6: a JVM file no project owns is the rest (2026-11-12)

Item 10 read a project as every file below its directory, so a file of a JVM kind there that is in
none of its source sets (a tool a script compiles and publishes to `mavenLocal()`, which another
project then takes as a jar) was a source on the project's own tests' lists only: a change to it
ran those, and reused the tests of the project that takes its jar. The decision: a project is read
as its source directories, every source set's (Java, Groovy, Kotlin, resources; Android's and
Kotlin's), the declared ones inside its directory and those outside it that are there, and as its
whole directory only when its source sets can't be told. So a file of a JVM kind that no project of
this build owns, below a project's directory or not, is below no directory a test read, and is the
rest: a change to one runs everything. A source's ownership has to come from the build, since only
the build knows which of its files it compiles, and what it doesn't compile may reach a test by a
road hushrun doesn't see. A second build of the repository was the rest already (a directory with
a settings file of its own). A task of one project (`:app:test`) keeps the sources of the projects
its classes read on their list, which a test checks.

## Amendment 7: a JVM that died, told by its failure's type (2026-11-12)

Amendment 3 told a test JVM that died, beside a genuine failure in the same task, only by the
words of Gradle's message, where no hook writes events (JUnit 4 on Gradle's own runner, TestNG).
The decision: the report also holds the exception class of the task's failure and of each cause,
the name that starts each failure's description (its stack trace), which no locale changes; a
`TestSuiteExecutionException` (Gradle's test executer, "an unexpected problem"), a
`ProcessExecutionException` (Gradle 9's test process that exited badly) or an `ExecException`
(the same before Gradle 9) says the task ran nothing whole. What was tried and not taken: the
build service's tooling events carry no exception object, only a message and a description;
`taskGraph.afterTask` gives the throwable but doesn't hold under the configuration cache; and a
count of the classes Gradle started against those it reported proves nothing either way: Gradle
ends the root suite and calls the test in flight skipped when its test process dies (Amendment
3), and a run showed the events of a JUnit 4 test that failed in the dying JVM lost altogether. The message words stay as the last signal,
for a failure whose description names no type. A run checks the mixed case, one class failing for
real and another's JVM dying in its class teardown, with each class in a JVM of its own, on
Gradle 8.14.5 and 9.3.1, and with the words switched off it holds by the type alone.

## Amendment 8: a test that ends its JVM, told by the hook as an agent (2026-11-12)

Amendments 3 and 7 needed the task to fail. A test that calls `System.exit(0)` under JUnit 4 or
TestNG on Gradle's own runners ends its JVM: Gradle calls the test in flight skipped, never
reports the rest of the class, and calls the task successful; those runners load no listener of
the hook, so no class was seen open, and the class was recorded as passed.

The decision: the script gives each test JVM of a run the hook's jar as a Java agent
(`-javaagent`, `Premain-Class` in its manifest). The agent transforms nothing: it writes that the
JVM began and registers a shutdown hook that writes that it ended, whether a thread called
`System.exit`, and the frames below that call. A JVM that began and never said it ended (a halt, a
kill, an out-of-memory death, Gradle's own timeout) ran nothing of its task whole; one a test
ended (frames of a test framework below the exit) leaves the classes whose frames are there not
run whole, or, when none is a class of the task, the whole task; and an exit that Gradle's worker
didn't call (a thread a test started) is the same as a halt. Gradle's worker ends a test JVM
normally by calling `System.exit` from `GradleWorkerMain`, which a run showed on Gradle 9.3.1, and
the Gradle tests show on 8.14.5.

What was tried, in the order asked: a TestNG listener through TestNG's own service loading would
have told TestNG's classes open, but JUnit 4 on Gradle's own runner loads nothing from the
classpath by itself, so it couldn't hold for both; the agent holds for both, and for the JUnit
Platform too, so it was kept alone. The conservative rule (a class with a skipped test under a
runner without events never reused whole) wasn't needed. Maven's Surefire already fails the
build when a test ends its fork (`The forked VM terminated without properly saying goodbye`), and
writes no report of that class: a test checks it.

## Amendment 9: a jar of the local Maven repository (2026-11-12)

A test classpath entry outside the repository and outside Gradle's user home was dropped without
a word: a jar a project of this build published to `mavenLocal()`, which another project takes by
its coordinates, was neither read nor not followed, and a change to the publishing project reused
the tests that ran its jar. The decision: an entry below a file-based Maven repository the build
resolves from (`mavenLocal()`, or any `maven { url = file(…) }`, a project's or the settings') is
never dropped: when its path's coordinates, `group:name`, are a project of this build, the class
reads that project as it reads a project dependency; else it is `not observed`,
`local-maven-repository`, naming the coordinates and the repository, since what built that jar
isn't known to hushrun. Gradle's own caches (`<Gradle user home>/caches/modules-2`) are fine as
they were: they hold what came from a remote repository, stored by its coordinates and checksum,
which no file of this checkout made. Maven's side: the extension names a reactor module by
`groupId:artifactId` without its version, so a dependency on a module's coordinates at another
version, resolved from the local repository, still reads that module's sources, which a test
checks.

## Amendment 10: Kotest's tests, never counted as reused (2026-11-12)

Item 7 let Kotest's tests run, since the hook has no method to leave them out by, but they were
recorded one by one, so the next plan listed those that passed and its line said they were
reused, and then they all ran. The decision: a test of a class whose tests the hook saw with no
method (Kotest's) is told never left out (C-39), so it records no pass of its own, no plan lists
it, and the line counts it as running. Its failure is recorded as before.

## Amendment 11: a jar a remote repository supplied is fine, wherever it lies (2026-11-12)

Amendment 9 made every jar of a file-based Maven repository whose `group:name` is no project of the
build `not observed`. A build that declares `mavenLocal()` and finds third-party jars there (a
Maven user's local repository holds JUnit) then lost all reuse: every class ran every time. The
rule was meant for a jar something on this machine built. The decision tells them apart by what
Maven and Gradle themselves leave beside an artifact, which runs showed: Maven 3.9's `install`
writes `maven-metadata-local.xml` beside the version directory and an empty remote for the file in
`_remote.repositories` (`thing-1.0.jar>=`); Gradle's `publishToMavenLocal` writes
`maven-metadata-local.xml` and no `_remote.repositories`; a jar Maven downloaded has its remote
named for the file (`junit-4.13.2.jar>central=`). So a jar whose `_remote.repositories` names a
remote for it, with no `maven-metadata-local.xml` and a version that isn't a `-SNAPSHOT`, is as
good as Gradle's own cache, and fine; any other (installed or published here, a snapshot, markers
missing or unreadable) reads the project with its coordinates where the build has one, and else
makes its class `not observed`. The reason is one sentence for `hushrun why`: what built the jars
(at most three named, and how many more) isn't known to hushrun; filter `mavenLocal()` to the
build's own group, or, where a remote repository has the same coordinates, which the person
checks, put `mavenLocal()` after it (for a jar only ever installed here, only the first helps). The cost: a jar installed by hand, or
by another build of this machine, still keeps its classes from reuse until the build stops taking
it from the local repository.

## Amendment 12: a file repository in the checkout (2026-11-12)

A jar of a file repository inside the checkout (`maven { url = file('localrepo') }`, git-ignored)
was a file of the checkout that git doesn't track: `not observed` for ever, though a project of the
build published it. Amendment 11's rules now come first there too: a jar a remote supplied is
fine, one a project of the build published reads that project, a file of it that git tracks is
read as a file, and any other is `not observed` for the same reason, in the same words.

## Amendment 13: a JUnit 4 suite's cases no test class owns (2026-11-13)

A JUnit 4 class run by `AllTests` (a static `suite()`, as Guava's testlib builds them) runs cases
whose class is no test class of its task, or none that loads. The script named a test class only
by the class an invocation names, so the suite class was no unit at all: its cases were dropped,
no count held them, and it ran every time unseen. The decision: the script also names the classes
of each invocation's parents, and a case whose class no test class of the task is belongs to its
nearest parent that is (the suite class that ran it), with its own id (`testlib.Checks.testOne`).
Such a class is told never left out, of itself and of each of its tests: it keeps no pass of its
own, runs every time, and no plan counts it as reused; its failures are its own, so the other
classes of a failed run keep their passes. Gradle doesn't run a class that only has a static
`suite()`, without `@RunWith(AllTests.class)` or `TestCase` as its superclass.

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
from the hook's events, which this adapter didn't read then (with them sent nowhere: 2,103; it
reads them since Amendment 3); on Gradle 8.14.5 they cost nothing measurable.

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
- A test class reused whole names no tests in that run: `units.tests` counts, for it, the tests
  this machine's store or a hint knows of it, as C-39 has it, beside the tests of the classes that
  ran.
- hushrun's extra input and output make a Test task's cache key differ from a build without
  hushrun: Gradle's build cache shares nothing between the two.

## Alternatives considered

- **An init script from the environment**: Gradle reads none; `GRADLE_USER_HOME` pointed elsewhere
  would move the person's caches, daemons and credentials.
- **Gradle's filter for every single test**: no pattern leaves out a nested class's method on
  Gradle 9, a plain one leaves out nothing under TestNG, and `*`-prefixed ones cost minutes.
- **Facts written in Groovy**: every rule above would live in an untested script.
