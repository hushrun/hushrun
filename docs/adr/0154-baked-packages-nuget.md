# 0154. Baked packages for NuGet: a fallback folder

- **Status:** proposed; being built
- **Date:** 2026-11-26
- **Rules:** K-4, K-5, K-6 and K-10 gain the `nuget` part (`docs/spec/11-baked-packages.md`);
  ADR 0149 and ADR 0118 read

## Context

ADR 0149 made a package store with Gradle and Maven parts. A .NET repository's sandbox still runs
`dotnet restore`, which downloads every package its projects name into NuGet's global packages
folder (`~/.nuget/packages`). The dotnet test adapter's fixtures (ADR 0118) need xUnit, NUnit,
MSTest and the test SDK before a test runs.

## Decision

1. **A `nuget` part, read in place (mechanism A).** It holds a global packages folder at
   `parts/nuget/packages`, and a run names it in `NUGET_FALLBACK_PACKAGES`. NuGet reads a fallback
   folder for what its global packages folder lacks and copies nothing from it.
2. **The variable replaces a config's fallback folders, so hushrun never hides one.** Where the
   person set `NUGET_FALLBACK_PACKAGES` already, their folders stay first and the part's joins
   after a `;`. Where a NuGet.Config names `fallbackPackageFolders` (the checkout's root or a
   folder above it, the user's, the machine's), the part is left alone, with a line. Carrying the
   config's list into the variable would mean resolving NuGet's whole config chain (`<clear />`,
   relative paths, `%VAR%`, per project); a list carried wrong drops a person's folder.
3. **What a part keeps.** Only files under `<id>/<version>/`, where a version starts with a digit:
   the `.nupkg`, its `.sha512`, the `.nuspec`, the extracted files, the package's
   `.signature.p7s`, and `.nupkg.metadata`, which NuGet writes and needs to see the package at
   all. Left out: `*.lock`, top folders whose name starts with a dot (`.tools`), and anything above
   a version's folder (a half-extracted folder under a random name).
4. **Verified by a signature.** NuGet doesn't hash what it reads from a fallback folder: with
   `--locked-mode` it compares the lockfile's hash with the `contentHash` in `.nupkg.metadata`,
   never with the bytes. A `nuget` part is `verified_by: signature`, as Maven's is.
5. **The bake** points NuGet at a folder of its own with `NUGET_PACKAGES`, which wins over a
   config's `globalPackagesFolder`; `--harvest nuget=FOLDER` takes a filled global packages folder.
6. **The manifest set** is the project files (`*.csproj`, `*.fsproj`, `*.vbproj`),
   `packages.lock.json`, `packages.config`, `Directory.Packages.props`, `Directory.Build.props`,
   `Directory.Build.targets`, `global.json`, NuGet.Config and `.config/dotnet-tools.json`: NuGet's
   lockfile is optional, so the build files count.

## What was confirmed by running the tool

On 2026-11-26, in a Linux container, with the .NET 8 SDK 8.0.134 (Ubuntu's package) and a project
of two packages (Newtonsoft.Json 13.0.3, Humanizer.Core 2.14.1) with a lockfile:

- A global packages folder filled online, copied and made read-only, named by
  `NUGET_FALLBACK_PACKAGES`, is read by an empty `NUGET_PACKAGES` with every source cleared: build
  and `restore --locked-mode` pass, and the empty folder gains no file. Without the variable the
  same restore fails (NU1100). `obj/project.assets.json` lists the fallback folder under
  `packageFolders`.
- **The variable replaces a NuGet.Config's fallback folders**, the project's and the user's: with a
  config naming the filled folder and the variable naming an empty one, restore fails. The
  separator is `;` (a `:` is taken as one path).
- Without `.nupkg.metadata` NuGet doesn't see a package (NU1100); without the `.nuspec` it fails
  (NU5037); without the `.nupkg`, its `.sha512` or `.signature.p7s` it builds.
- A DLL in the fallback folder changed in place, same size: `restore --locked-mode` passes. The
  `contentHash` in `.nupkg.metadata` changed: it fails (NU1403).
- With a config's `globalPackagesFolder` and `NUGET_PACKAGES` both set, NuGet fills the
  variable's folder.

The integration test (`crates/hushrun/tests/packages_nuget.rs`) runs the dotnet adapter's fixture
offline through `hushrun run` with the store and then without it: the second run skips, so the
part enters no key. `obj/project.assets.json`, which names the store's folder, is never tracked,
and the adapter's keys hold only tracked files a test project compiles (ADR 0118).

Not confirmed here: `nuget.exe` and `msbuild -t:restore` outside the SDK; NuGet on macOS and
Windows (where the separator may differ); an SDK older than 8.

## Considered and dropped

- **`RestoreAdditionalProjectFallbackFolders`** from the environment, which MSBuild adds to a
  config's folders instead of replacing them: it reaches only MSBuild's restore, and an
  environment variable read as an MSBuild property changes the project's evaluation.
- **Seeding the global packages folder** (mechanism C): NuGet reads a fallback folder natively, so
  nothing needs copying.
- **Leaving the part alone whenever `NUGET_FALLBACK_PACKAGES` is set**, as Gradle's part does with
  its variable: the variable is a list, and joining it keeps every folder the person named.

## Consequences

- A project's own NuGet.Config below the checkout's root that names fallback folders isn't looked
  at; the variable replaces them, and NuGet downloads what they held.
- `hushrun packages bake -- dotnet test` keeps what the build downloaded; the store's size grows
  with the SDK's own packages a build restores (test SDK, analyzers).
