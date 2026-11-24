# 0117. Kotlin: Multiplatform's own test tasks, Kotest's specs, and what Kotlin's tasks share

- **Status:** accepted
- **Date:** 2026-11-24
- **Rules:** C-94 to C-97, B-210; relies on C-30, C-32, C-39, C-46, B-79, B-83, ADR 0087, 0104

## Context

Kotlin on the JVM is Gradle's, and both of hushrun's Gradle features already took it: a test
class of a `Test` task is a unit named by its source file, `.kt` files included, and its methods
are tests ([ADR 0087](0087-gradle.md)); Gradle's tasks, `compileKotlin` among them, are shared
through hushrun's remote build cache ([ADR 0104](0104-gradle-build-blocks.md)). An audit on Kotlin
2.3.21, Gradle 8.14.3 and 9.3.1, JDK 21, found what was left:

- **Kotlin Multiplatform's tests on other platforms were not followed.** `jvmTest` is a `Test`
  task, and was covered. `jsNodeTest`, `jsBrowserTest` and `wasmJsNodeTest` are `KotlinJsTest`,
  and `linuxX64Test`, `macosArm64Test` or `iosSimulatorArm64Test` are `KotlinNativeTest`: each an
  `AbstractTestTask` that is no `Test`, which ADR 0087's item 4 named and left out. The run said
  `not followed` for each, ran all their tests every time, and recorded nothing of them: a
  `commonTest` class reused on the JVM still ran on JavaScript and every native target.
- **Those tasks are close to a `Test` task where it matters.** A run of a `jsNodeTest` showed
  Gradle's own test events, each test's class (`p.CalcTest`) and method (`adds`, displayed
  `adds[js, node]`), under a suite named after the task as well (`jsNodeTest.p.CalcTest`); and
  KGP reads the task's filter at execution (`KotlinTest.getExcludePatterns`, from `filterExt`) and
  hands it to Kotlin's test runner, so a pattern added by the task's first action leaves a class
  (`p.MoreTest`, `p.MoreTest.*`) or a test (`p.CalcTest.adds`) out, under the configuration cache
  too. They run no class file of the build: there is no `testClassesDirs` to exclude from, no
  `SourceFile` attribute to name a class by, and no JVM for the hook.
- **A Kotlin/JS test that ends its process fails its task.** `js("process.exit(0)")` in a test
  made KGP fail the task (`Test running process exited unexpectedly. Current test: b`) and the
  test in flight with it, and the tests after it never reported: no class records a pass it
  didn't earn, without a hook.
- **Kotest's specs were already units.** Gradle sends Kotest's JUnit Platform engine one class
  selector per test class it found, and the engine runs only those: a spec left out by ADR
  0087's class exclusion doesn't run, with `unit: file` and `unit: test` alike. Its tests have no
  method a filter could name, and stay never left out (ADR 0087, Amendment 10).
- **Kotlin's compile tasks travel.** Two checkouts at different paths, with Gradle's local cache
  off so that only hushrun's served them, had the same build cache key for every task of a
  build with `kotlin("jvm")`, kapt (`auto-service`) and KSP (`moshi-kotlin-codegen`):
  `compileKotlin`, `kspKotlin` and `kaptKotlin` came from the second machine's cache, and their
  entries, canonical, held no folder of the machine. One task stayed: kapt's
  `kaptGenerateStubsKotlin` writes each stub's `.kapt_metadata` with the absolute path of its
  Kotlin source (`KaptStubConverter.KaptStub.writeMetadataIfNeeded`, written whatever
  `mapDiagnosticLocations` says), so B-79 keeps it on the machine. The run's line said `1 task run
  here stays on this machine (1: its outputs hold the checkout's folder)`, and not which.
- **Android's local unit tests** (`testDebugUnitTest`) are `Test` tasks, followed since ADR 0087,
  their Kotlin source directories read from Android's source sets; nothing new here.
- `hushrun init` proposed nothing for a CI that runs `./gradlew allTests`, `jvmTest`, `jsTest` or
  `testDebugUnitTest`, only for `test`, `check` and `build`.

## Decision

1. **Kotlin's own test tasks are followed** (C-94): every `KotlinJsTest` (Kotlin/JS and
   Kotlin/Wasm, Node.js or a browser) and every `KotlinNativeTest` of the build is configured as a
   `Test` task is, by the same script, with what a `Test` task gets but the class exclusion and
   the hook. Any other `AbstractTestTask` stays `not followed`, as before. A task is told by its
   class or a superclass's name, so the script compiles against no Kotlin plugin.
2. **A class is named by the file that declares it** (C-95). The task's classes are those a `.kt`
   file under the project's test source directories (ADR 0087's: Kotlin's `*Test` source sets,
   `commonTest`, `jsTest`, `nativeTest`… among them) declares at its top level, by its package;
   the file when exactly one declares it, else `<project directory>::<class>`, as a class of no
   file is for a `Test` task. A class the task reports that no file declares at its top level (a
   nested one) is its outer class's. So a `commonTest` class is one unit for `jvmTest`,
   `jsNodeTest` and `linuxX64Test`, its tests one test each, as ADR 0087 already holds two tasks
   that ran one class (a test finished where every task that ran it did). A suite named
   `<task>.<class>` is the class's own suite, so the test keeps its method's name.
3. **Leaving out is Gradle's filter** (C-96), added by the task's first action from the plan read
   then: a class left out whole as `<class>` and `<class>.*`, a test as `<class>.<method>`, at most
   250 tests a task, in the order of their ids, which the facts say (`leaveOutAtMost`, ADR 0087's
   Amendment 15), since every pattern goes on Kotlin's runner's command line. A class of the
   default package, or of a package whose name starts upper-case, is never left out, since Gradle
   reads such a pattern as a simple class name. What a task leaves out is its input
   (`hushrunLeftOut`), so another list runs it again; a task none of whose classes the check
   knows executes, as ADR 0087's Amendment 16 has it.
4. **Outcomes are Gradle's events** (C-96), as for a `Test` task, with no hook events: a task's
   root suite that ended ran its classes whole, a task that failed with no failed test or suite
   ran nothing whole, and a test process that ends early fails the task and its test in flight
   (KGP's own), so nothing of it records a pass.
5. **What a class read** is its project's source directories and those of each project it declares,
   transitively (ADR 0087's item 10 and Amendment 2), with no classpath to map. Kotlin/JS's
   `kotlin-js-store/yarn.lock`, a project's `package.json` from its build script, and a native
   target's `src/nativeInterop/cinterop/*.def` are in no source set: the rest, so a change to one
   runs everything.
6. **Kotest's specs** (C-97) stay what ADR 0087 made them, now held by a test: a spec is a unit,
   left out whole by the class exclusion when what it read didn't change; its tests aren't left
   out one by one.
7. **The line names what stays** (B-210): `build: gradle: N tasks run here stay on this machine
   (…): :kapt:kaptGenerateStubsKotlin`, three tasks at most and how many more, so a Kotlin build
   sees that kapt's stubs, not its own task, are what stays. hushrun doesn't rewrite them: B-79
   searches an entry's bytes for the machine's folders because a path in an output may be read,
   and kapt maps its processors' diagnostics through those paths. KSP's outputs hold none; the guide
   says so.
8. `hushrun init` takes `allTests`, `jvmTest`, `jsTest`, `testDebugUnitTest` and
   `testReleaseUnitTest` as Gradle test tasks, as it takes `test` (C-94).

## Consequences

- A Kotlin Multiplatform build reuses its tests on every platform it runs them on: a change to
  one project's code runs the classes that read it on the JVM, on JavaScript and on each native
  target, and leaves out the rest on all of them.
- Two classes of different files with one name (`expect`/`actual` test classes) can't be told
  apart from a Kotlin/JS or native task: each is `<project directory>::<class>`, whose row the
  store keeps only while it runs (ADR 0087's item 5).
- A Kotlin/JS or native task leaves out at most 250 tests one by one, and no class of the default
  package; the rest run, and the plan counts them as running.
- Not verified here, since this machine's network refuses `download.jetbrains.com` and
  `codeload.github.com`: Kotlin/Native (its toolchain comes from the former; CI's Linux job runs
  the test with `HUSHRUN_TEST_KOTLIN_NATIVE=1`), Kotlin/Wasm (its tooling from the latter; the same
  `KotlinJsTest`), a browser's `jsBrowserTest` (Karma, its own runner), and Android with Kotlin
  sources (CI's Android test is Java's).
- Kotest's multiplatform engine (its own `kotest` tasks for JavaScript and native) is not a
  `KotlinJsTest`: not followed, said so.

## Alternatives

- **Naming a Kotlin/JS class by the JVM task's class files**: holds only where the build has a JVM
  target, and a JS-only or native-only build has none.
- **Leaving a class out by the source files the compilation reads**: a test compilation of
  Kotlin/JS or Native links one binary from every file, so removing one is a new binary and a new
  compile, which costs more than the tests it saves.
- **A hook for Kotlin's runners**, as the JVM has: Kotlin's runners already report every test to
  Gradle and fail the task on a process that ends early, which is what the hook adds on the JVM.
- **Rewriting kapt's absolute paths before an entry leaves**: an entry hushrun changed is no longer
  what Gradle stored under its key, and the paths are what kapt reads back.
