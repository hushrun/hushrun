# 0118. `dotnet test`'s test projects, and each test method

- **Status:** accepted
- **Date:** 2026-11-24
- **Rules:** C-98, C-99, C-100, C-101; relies on C-22, C-30, C-31, C-32, C-33, C-34, C-38, C-39,
  ADR 0068, 0069, 0080, 0090, RFC 0004

## Context

RFC 0004 asks for the languages a check reuses test by test, and .NET's (C#, F#, Visual Basic) run
their tests with `dotnet test`: before this decision `hushrun init` proposed it as a whole check,
and any COMMAND of it got the pytest adapter, which observes nothing there. `dotnet test` builds
each test project with MSBuild and calls the SDK's `VSTest` target on it, which starts VSTest
(`vstest.console`) on the project's assembly; xUnit, NUnit and MSTest are VSTest adapters inside
it. Like CTest's (ADR 0090), its tests are in what a build makes, and what one read is what it is
built from; unlike CTest, MSBuild reads its targets from files the environment can name. A spike
on the .NET 8 SDK (8.0.134, MSBuild 17.8) with xUnit 2, NUnit 4 and MSTest 3 proved what follows.

## Decision

1. **A unit is a test project for one target framework and configuration, a test is a test
   method** (C-30, C-39, C-98). The unit is the project file's path from the root with both,
   `tests/Calc.Tests/Calc.Tests.csproj (net8.0, Debug)`, so that a multi-targeted project's builds
   and a Debug and a Release build never share a result. A test's id is its fully qualified name
   as VSTest's TRX file gives it, `<className>.<name>`: `Calc.Tests.AddTests.Adds`, NUnit's test
   case `Calc.Tests.AddTests.Zero(1)`. An xUnit theory's and an MSTest data-driven test's rows share
   it: they are one test, which passed when every row did. In what people read a test project is a
   test file, as a Go package is.
2. **Loading, with COMMAND byte for byte as written.** The SDK imports
   `Microsoft.TestPlatform.targets` by the `VSTestTargets` property, which MSBuild takes from the
   environment. hushrun names a file of its own there, in the run's directory, which imports the
   SDK's (or the file COMMAND's environment named) and, in a run, replaces the `VSTest` target of
   each test project (the SDK imports it after every other, so the last definition is hushrun's).
   Every `dotnet test` COMMAND starts is observed, a script's included. A project of another kind,
   `dotnet test --list-tests`, and anything that isn't a test project run the SDK's target
   unchanged.
3. **What a test project read is what MSBuild says it compiles.** Before its build, hushrun's
   target asks the project, and each project it references to the end (resolved as MSBuild's own
   `ResolveProjectReferences` resolves them, target framework included), for its `Compile` items
   and its project file, in instances of their own (a global property no build has): run in the
   build's own instance, the reference targets ran before `AssignProjectConfiguration` and the
   build then compiled without its references, which the spike hit. **Sources** are the files of a
   C#, F# or Visual Basic kind a project compiles (`.cs`, `.fs`, `.fsi`, `.vb`); project files,
   `Directory.Build.*`, `Directory.Packages.props`, lock files, resources and data are in the rest
   set, so a change to one runs everything. A test project's own `Compile` items are its unit's
   files (the units set). Files outside the root (a package's `Program.cs`) aren't read.
4. **Selection, checked where it happens.** The plan keys each unit by the map's reads and writes
   the lists it keyed each unit by to `dotnet-plan.v1` in the run's directory, as CTest's (ADR 0090
   decision 8): it can't know before COMMAND what MSBuild will compile. Before each test project's
   build, hushrun's target starts `hushrun __hushrun-dotnet before`, which leaves a unit out only
   when the source files of the scope it compiles now are exactly those of every list its key was
   made from. A unit `reuse` lists is skipped whole: neither built nor tested. Otherwise the tests
   `reuseTests` lists are left out by `FullyQualifiedName!=<id>`, joined by `&`, after COMMAND's own
   `--filter` as `(<COMMAND's>)&(<hushrun's>)`; an id that would need VSTest's `\` escape (`(`, `)`,
   `=`, `!`, `~`, `&`, `|`, a space; NUnit's test cases) is never left out, since MSBuild turns `\`
   into `/` on Linux and macOS, and the spike's filter then matched nothing. Past 60,000 bytes
   nothing is left out one by one.
5. **The SDK's own target does the work.** hushrun's `VSTest` builds the project as the SDK's
   does, then calls the SDK's `VSTest` on the same project again with `VSTestNoBuild`, in an
   instance without COMMAND's `VSTestLogger` and `VSTestTestCaseFilter`, which a target of
   hushrun's sets there from files `before` wrote, one logger a line: global properties can't be
   changed, and a list of loggers doesn't survive the MSBuild task's `Properties` (the spike merged
   them into one invalid logger). Every other option COMMAND gave (`--collect`, `--blame`,
   `--settings`, `-- <runsettings>`) reaches VSTest as before. The exit code and what a person sees
   are the SDK's but for one more `Results File:` line per project.
6. **Outcomes from a TRX file.** hushrun adds VSTest's TRX logger with a file in the run's
   directory; VSTest keeps one TRX logger, so when COMMAND has its own (`--logger trx`), that one is
   kept and read: of the `.trx` files written since the project's tests started in its results
   directory, the one whose tests are all in its assembly. Nothing ever reads as a pass from
   absence: the file must parse and hold as many top-level results as its counters' `total`; a
   result names a test its definitions hold. `Passed` passed; `Failed`, `Error`, `Timeout`,
   `Aborted` failed; `NotExecuted` was skipped, which passed; any other didn't finish. MSBuild's
   call of the tests failing with no test failed (a test host that crashed) fails the project.
   `hushrun __hushrun-dotnet after` writes the facts; a project whose `after` never ran left its
   `.started` file, and the run records no pass (C-32).
7. **A stale assembly ran old code.** An assembly older than a file it compiles (`--no-build` after
   an edit) ran old code: the run records no pass, and says which.
8. **Which COMMANDs.** `dotnet test …`, by name or path (loading needs neither). Refused, said once,
   as C-32's `test files aren't observed: <why>`: `dotnet test <assembly>.dll`, which runs VSTest
   without MSBuild, and a `global.json` above COMMAND's directory that sets `test.runner` to
   `Microsoft.Testing.Platform` (.NET 10's mode, which starts no `VSTest` target).
9. **Microsoft.Testing.Platform projects aren't observed yet** (C-101). A project with
   `TestingPlatformDotnetTestSupport` (MSTest's, NUnit's or xUnit v3's runner) replaces the SDK's
   `VSTest` target with its own after hushrun's, and runs its test application itself; its results
   are in a TRX file only with an extension package it may not have, and its filters differ by
   framework. hushrun's target before `InvokeTestingPlatform` says what it compiles: it is a unit
   that is `not observed` and always runs, its files are sources, so a change to one runs it alone,
   and the run says once `<project> runs on Microsoft.Testing.Platform, whose results hushrun
   doesn't read yet: its tests aren't observed`.
10. **Tests one by one** (C-39): with `unit: test` the adapter reports each test and leaves out the
    ones `reuseTests` lists; a test can't be watched (`reads: test` runs as `reads: file`): each test
    read what its project compiles.
11. **Compiled code isn't reused** (no B rule; B-220 to B-229 stay reserved for it). A spike on the
    same SDK measured what a `csc/1` kind, a compiler launcher in the shape of C and C++'s
    (ADR 0097), would get, and found the SDK defeats its main use:
    - **The way in works, and its cost can be avoided.** MSBuild's `Csc` task starts the program
      `CscToolPath` and `CscToolExe` name, from the environment, as `<program> /noconfig
      @<response file>`, the whole compile in the file. It then no longer uses the compiler server
      (`VBCSCompiler`): a launcher that starts the compiler as given made a warm clean build of
      five projects take 8 s instead of 1.6 s, about 1.3 s more for each compile not reused. With
      `/shared` added, the compile goes to the server, and the build took 2.0 s.
    - **A block leaves its folder only with `ContinuousIntegrationBuild`.** Two git checkouts at
      different paths, each built with `ContinuousIntegrationBuild=true` (the SDK then maps the
      repository's root, from SourceLink), gave byte-identical assemblies and PDBs. Without it, the
      assembly holds its PDB's absolute path, and a block would be tied to its folder (B-56), as on
      a CI runner, whose path is the same for every job.
    - **Every commit changes every compile.** Since .NET 8 the SDK writes the commit into each
      assembly: `IncludeSourceRevisionInInformationalVersion`, on by default in a git checkout,
      makes the generated `AssemblyInfo.cs` hold `AssemblyInformationalVersion("1.0.0+<commit>")`,
      and SourceLink's JSON, another input of the compile, names the commit too. A key of the
      bytes the compiler reads, the only sound one, so changes for every project at every commit,
      and a block is never reused across commits: not by a branch's first CI job from `main`'s,
      nor by a worktree on another commit. What would still be reused is the same commit built
      again: in another worktree or clone, a re-run CI job, a return to a commit, and the
      repositories that turn the stamp and SourceLink off.
    - **hushrun doesn't change what a build makes.** Leaving the commit out of the key, or
      setting the property for COMMAND, would restore blocks whose version string and source
      links name another commit than the one built, or change every assembly's version string.
    - MSBuild's project cache plugins (`Microsoft.Build.ProjectCache`, as microsoft/MSBuildCache
      uses) would cache whole projects, but load a .NET assembly that hushrun would have to build
      and ship for every SDK, and meet the same stamp.

    Reuse of the same commit alone doesn't justify a launcher, key and store kind of C and C++'s
    size. Test projects are still reused (decisions 1 to 10), and an unchanged test project isn't
    even built.

## Consequences

- .NET repositories reuse test projects, and test methods one by one, on the same rules as Rust's
  test binaries, Go's packages and CTest's programs; a solution's test projects whose key passed
  are neither built nor tested.
- A change to any project file, `Directory.Build.*`, package version or lock file runs everything:
  they are in the rest set, as CMake files are for CTest.
- Compiler options, the SDK's version and packages' contents aren't in a key, as `RUSTFLAGS` aren't
  in Cargo's (C-9 is the check's own key's limit); the package versions are, through the project
  files in the rest set.
- Files a test opens at run time aren't followed; those in the repository are in the rest set.
- Test methods whose fully qualified names VSTest can't match exactly (NUnit's test cases) run
  whenever their project does.
- Microsoft.Testing.Platform projects, `dotnet test` of an assembly and .NET 10's Microsoft.Testing
  .Platform mode run as COMMAND says, unobserved, and the run says so.
- `dotnet build` compiles as before, inside a run or not: no compile is reused (decision 11).

## Alternatives considered

- **A `dotnet` of hushrun's on `PATH`** (ADR 0080's programs, as CTest's): it would rewrite
  `--filter` and `--logger` on the command line, but `dotnet` is often started by its path
  (`$DOTNET_ROOT/dotnet`, setup actions), a solution's projects share one filter (a test project
  couldn't be skipped whole, nor built less), and the command line knows nothing of what MSBuild
  compiles.
- **`CustomAfterMicrosoftCommonTargets`:** it is imported before the SDK's VSTest targets, whose
  `VSTest` target would replace hushrun's.
- **Setting `VSTestTestCaseFilter` and `IsTestProject` in a target before the SDK's `VSTest`:** a
  property COMMAND's `--filter` or `--logger` gave is global and can't be changed.
- **Listing the tests first (`--list-tests`):** it names a test by its display name, not the name a
  filter matches, and costs a second start of VSTest for every project.
- **A unit per test class:** VSTest's filter can leave a class out with `FullyQualifiedName!~`, but
  `~` is a substring and leaves out more than the class; a method's exact name can't.
