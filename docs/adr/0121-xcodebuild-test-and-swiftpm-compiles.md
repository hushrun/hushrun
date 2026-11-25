# 0121. `xcodebuild test`'s test targets and tests, and SwiftPM's compiles

- **Status:** proposed; the `xcodebuild test` adapter is built and tested on CI's macOS runner;
  SwiftPM's compiles (part 2) are decided below and not built
- **Date:** 2026-11-24
- **Rules:** C-110 to C-113; B-250 to B-259 reserved for part 2; relies on C-22, C-30 to C-39,
  C-48, ADR 0080, 0089, 0105

## Context

Swift had two adapters before this: `swift test`'s test targets and tests (ADR 0089), and Xcode's
compilation cache served by hushrun (ADR 0105). Two gaps were left:

1. **`xcodebuild test`** wasn't observed. iOS, iPadOS and macOS apps test with Xcode: an app's
   unit tests are hosted in the app, its UI tests drive it, and neither runs under `swift test`.
   ADR 0089 sketched the adapter and left it unbuilt.
2. **SwiftPM compiled outside Xcode** (`swift build`, `swift test` on Linux and macOS) reuses
   nothing between worktrees or machines: a server-side Swift package's CI job starts from an
   empty `.build` every time.

This container couldn't install Swift (download.swift.org is closed to it), so everything here was
built against what Xcode and SwiftPM are documented and known to do, and tested on CI's macOS
runner, which has Xcode.

## Decision, part 1: `xcodebuild test`

1. **A unit is a test target** (C-110): a unit-test or UI-test bundle, named
   `<project from the root>/<target>`, since targets are named in the project and needn't have a
   folder of their own (`ios/App.xcodeproj/AppTests`). For a Swift package tested through its Xcode
   scheme (`xcodebuild -scheme Pkg-Package test` in a folder with a `Package.swift` and no
   project), the package's test targets as `swift test`'s adapter names them, by their
   directories. A **test** is its `nodeIdentifier` in the result bundle: XCTest's
   `Class/testMethod()`, Swift Testing's `Suite/test()` or `test()` outside a suite. A
   parametrized Swift Testing test is one test, as in ADR 0089.
2. **Reads come from `project.pbxproj` alone** (C-111), read by a parser of the old-style
   property list written for it (`pbxproj.rs`): nothing is built, listed or resolved before
   COMMAND, so `hushrun ready` plans a project's units from the files, as it does pytest's. A
   target reads the files its build phases name, its file-system-synchronized folders (Xcode 16's
   default for new targets) and those another target's exceptions add to it, the local Swift
   packages it uses, and all of that for each target it depends on: a target dependency, a product
   it links (an implicit dependency) and the app that hosts its tests (`TestTargetID`). A target
   with a run-script phase reads its project's folder whole, as a SwiftPM target built with a
   plugin reads its package whole (ADR 0089): a script can read anything. What can't be followed
   is said and makes the unit `not observed`: a source outside the scope or the checkout, a target
   of another project (subprojects), a remote package with no `Package.resolved`, a package
   product of no package the workspace holds. Sources and the rest set are split as ADR 0089's
   Amendment 2 splits them: a file of a compiled kind that some test target reads is a source,
   everything else (the project file, schemes, xcconfig files, assets, `Package.resolved`) is the
   rest set, so a change to it runs every test target.
3. **Loading** is ADR 0080's program on `PATH`, named `xcodebuild` (C-110). It catches
   `xcodebuild` typed, or started by name from a shell's `-c` line, a script or a Makefile whose
   own text starts `xcodebuild … test` (C-32's wrapper rule). fastlane's `scan` starts it too, but
   a COMMAND `fastlane scan` names no runner hushrun knows, so nothing loads.
   `xcrun xcodebuild` and a path to `xcodebuild` go round it: as COMMAND they are refused, said
   once. Standard output and error aren't touched; the exit code and a stopping signal are the
   real one's.
4. **Leaving out** (C-112) is `-skip-testing:<target>` for a target and
   `-skip-testing:<target>/<id>` for a test, literal identifiers Xcode matches as written, so
   nothing is escaped. hushrun's `xcodebuild` leaves out only units of the projects (or package)
   that start tests, which the plan writes to `xcodebuild-plan.v1` beside the run: a script that
   tests two projects leaves out in each only what is the plan's for it, and a target name two
   units share is never left out, since `-skip-testing` names a target by its name alone.
5. **Outcomes** come from a result bundle, hushrun's own in the run's directory unless COMMAND
   names one (read only when this run wrote it), through `xcrun xcresulttool get test-results
   tests` (Xcode 16 and later; older Xcode is refused). `Passed` and `Expected Failure` passed,
   `Skipped` was skipped (it records no pass in a run that failed, C-32), and anything else
   failed; a failure on any device, configuration or repetition fails the test. Never a pass from
   absence: a test the bundle doesn't name isn't told, but for one hushrun left out, which is
   deselected. A run that failed with no test failing (a build error) keeps no pass, and a bundle
   that can't be read records nothing; when tests were left out, the run starts COMMAND once more
   with nothing left out (C-32).
6. **Refused, said once** (C-110): `test-without-building` and `-xctestrun`, which run the test
   products an earlier build left (as `swift test --skip-build`, ADR 0089 Amendment 1);
   `-test-iterations`, `-retry-tests-on-failure` and `-run-tests-until-failure`, which run a test
   more than once; any system but macOS.

### Not done in part 1

- **Listing before or after the run.** `xcodebuild -enumerate-tests` builds first, as `swift test
  list` does, which ADR 0089 decided against before the run; after it, the bundle is the list.
  So a check's map holds the tests that ran, not the tests a test plan disabled.
- **Test plans' own selection** (`-testPlan`, `skippedTests` in an `.xctestplan`): what they
  leave out doesn't run and isn't told; hushrun's `-skip-testing` adds to theirs.
- **A per-file graph of Xcode's own build** (what Xcode actually compiled for each target, from
  its build description): the project file says the same for what a test target reads, without a
  build.

## Decision, part 2: SwiftPM's compiles (designed, not built)

The obvious routes, and why none is built yet:

- **A `swiftc` wrapper, as for C and C++** (ADR 0097): SwiftPM honours `SWIFT_EXEC`, but derives
  the toolchain's other tools and its resource folder from that path, so a wrapper outside the
  toolchain breaks the build; `SWIFT_DRIVER_SWIFT_FRONTEND_EXEC` reaches each frontend job, but a
  frontend job's inputs are every source of its module, every module it imports, Clang's implicit
  module cache and the supplementary output maps the driver writes to fresh temporary files: a
  correct key needs what swiftc's own caching already computes.
- **swiftc's compilation caching** (`-cache-compile-job`, explicit modules, `-scanner-prefix-map`):
  the compiler keys each job by its content and asks a CAS, a folder (`-cas-path`) or a plugin
  (`-cas-plugin-path`). On macOS, Xcode's toolchain plugin speaks the protocol hushrun's Xcode
  service already serves (ADR 0105), so SwiftPM's compiles would be shared as `xcode/1` blocks with
  no new block kind; hushrun's `swift` on `PATH` (ADR 0089) would add the flags to `swift build`,
  `swift test` and `swift run`. On Linux, the open-source toolchains ship no such plugin: a local
  CAS in the store would give reuse between worktrees on one machine, and sharing would need a
  plugin of hushrun's own (the LLVM CAS plugin C API, a shared library in every release).

This is the route chosen, in that order: macOS through ADR 0105's service first, then Linux's
local CAS, then a plugin for sharing on Linux. It isn't built here because none of it could be run
from this container, and the flags, SwiftPM's handling of explicit modules and the plugin's
options must be measured on a real toolchain first: a hushrun that adds wrong flags breaks a
build, which is worse than reusing nothing. The rules B-250 to B-259 are reserved for it. What a
first spike on CI's runners must answer:

1. Whether `swift build -Xswiftc -cache-compile-job -Xswiftc -cas-path …` (with
   `-explicit-module-build`, or SwiftPM's `--experimental-explicit-module-build`) builds a package
   with a C target and a macro, and hits in a second checkout with `-scanner-prefix-map` for the
   checkout and `.build`.
2. Whether the same build with Xcode's `libToolchainCASPlugin.dylib` and
   `remote-service-path=<socket>` reaches hushrun's service, and whether its values name their
   outputs as Xcode's do (ADR 0105's spike, question 3).
3. Whether flags that change between a run with hushrun and one without make SwiftPM rebuild the
   whole `.build` (a person's `swift build` outside hushrun after one inside).

## Consequences

- An Xcode project's test targets are reused on what the project says they are built from, and
  with `unit: test` its tests one by one; `hushrun ready` says which a run would run.
- A project whose targets all have a run-script phase (a linter in every target) reads its folder
  whole in every target: each change runs every test target, as before this adapter. Moving the
  script to a target the tests don't depend on, or to an aggregate target, gives reuse back.
- `xcodebuild test` takes, besides, one `xcodebuild -version` and one `xcresulttool` after the run.
- SwiftPM's compiles still reuse nothing between checkouts: part 2 is the plan.

## Alternatives considered

- **`-only-testing` for what runs**, instead of `-skip-testing` for what doesn't: a test added
  since the plan would never run.
- **The scheme's testables as the units**, from `.xcscheme` files: a scheme isn't always shared
  (`xcuserdata`), and Xcode makes schemes it doesn't write; the project's targets are always there.
- **The console for outcomes**, as serial XCTest under `swift test`: Swift Testing's lines differ
  between Xcode versions, and the bundle is what Xcode itself reads.
