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
2. **The variable replaces a config's fallback folders, so hushrun carries them.** Where the
   person set `NUGET_FALLBACK_PACKAGES` already, their folders stay first and the part's joins
   after a `;`. Otherwise the folders the NuGet.Configs name (the checkout's root and each folder
   above it, the user's, the machine's) go first, merged as NuGet merges them (confirmed below),
   then the part's. A list hushrun can't read exactly (`%VAR%`, an XML entity, `<remove>`) leaves
   the part alone, with a line, rather than drop a folder.
3. **What a part keeps.** Only files under `<id>/<version>/`, where a version starts with a digit:
   the `.nupkg`, its `.sha512`, the `.nuspec`, the extracted files, the package's
   `.signature.p7s`, and `.nupkg.metadata`, which NuGet writes and needs to see the package at
   all. Left out: `*.lock`, top folders whose name starts with a dot (`.tools`), and anything above
   a version's folder (a half-extracted folder under a random name).
4. **Verified by a signature, with or without `packages.lock.json`.** The plan asked for the
   lockfile class where a lockfile exists. It can't hold: NuGet doesn't hash what it reads from a
   fallback folder (with `--locked-mode` it compares the lockfile's hash with the `contentHash` in
   `.nupkg.metadata`, never with the bytes), and the lockfile's `contentHash` is the SHA-512 of the
   `.nupkg`, while NuGet reads the files extracted beside it. hushrun checking the `.nupkg` against
   the lockfile would leave every DLL a build loads unchecked. So a `nuget` part is
   `verified_by: signature`, as Maven's is, and its rows carry no `locked`.
5. **The bake** points NuGet at a folder of its own with `NUGET_PACKAGES`, which wins over a
   config's `globalPackagesFolder` (the plan's `dotnet restore --packages` does the same for one
   command; the variable reaches whatever the check's own command runs); `--harvest nuget=FOLDER` takes a filled global packages folder.
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
- **How NuGet merges configs' fallback folders:** with the user's config naming `a=/f1`, `b=/f2`
  and the project's `c=f3rel`, `a=/f3`, `packageFolders` lists `<project>/f3rel`, `/f3`, `/f2`: the
  closest config's first, in its order, a key it named skipped further out, a relative folder
  from its config's folder. A `<clear />` in the project's config drops the user's.
- Restore and build never write the fallback folder: its files' SHA-256s are the same after both,
  with the folder writable.
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
- **Leaving the part alone whenever a config names fallback folders**: it would turn the part off
  for every team that keeps one; carrying the list keeps both.
- **The keys test in `units_dotnet.rs`**, as the plan placed it: that file is in-process, and only
  a run of the binary records the map whose keys the test reads, so it is in `packages_nuget.rs`.

## Consequences

- A project's own NuGet.Config below the checkout's root that names fallback folders isn't looked
  at; the variable replaces them, and NuGet downloads what they held.
- NuGet runs no install script for a `PackageReference`, but a package's MSBuild targets run
  during a bake's build and could write under a package's folder in the bake's `NUGET_PACKAGES`;
  the part would keep that file. Nothing here tells it from an extracted file.
- `hushrun packages bake -- dotnet test` keeps what the build downloaded; the store's size grows
  with the SDK's own packages a build restores (test SDK, analyzers).
