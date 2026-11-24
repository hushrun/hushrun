# 0111. What a real Go module showed: coverage, files no test binary is built from, and the standard library's assembly

- **Status:** accepted
- **Date:** 2026-11-24
- **Rules:** C-70, C-71, B-150; amends C-30 for Go, B-42 and B-45; relies on ADR 0070, 0086, 0093

## Context

ADR 0070 and 0086 reuse Go's tests by package and one by one, and ADR 0093 shares Go's pure-Go
package archives through Go's cache program. This decision is what an audit of both on a module
built to have every shape found: `go test` with `-race`, `-short`, `-cover`, `-coverprofile`,
`-coverpkg`, build tags on the command line and in `GOFLAGS`, a fuzz target with a seed corpus
under `testdata/fuzz`, a cgo package, a `go.work` of two modules tested from the root and from one
member, and `go vet` (Go 1.24.7 here; CI runs 1.27.4).

What held, unchanged:

- `-race` and `-short` reuse as any flag does: COMMAND is in every key, and `-race`'s `race` tag is
  one `go list` is asked with (C-32).
- A seed added to or changed in `testdata/fuzz/FuzzX` reruns every package, since the corpus is
  in the rest set; its failure was caught, never reused.
- `-tags` on the command line and in `GOFLAGS` choose the files `go list` names, and a file the tags
  leave out stays out of the packages (ADR 0086 Amendment 5). `GOFLAGS` is not in the check's key,
  as no variable is (the run guide's "What counts as the same code").
- A package whose tests link the repository's cgo is `not observed` (ADR 0070) and runs every time;
  cgo's C compiles stay in Go's own cache (ADR 0093: its id doesn't hold the C toolchain's headers).
  cgo's compiler doesn't go through hushrun's C launcher, which only CMake starts.
- In a `go.work`, testing from the root reads each module's packages, and a change in one module
  reruns its packages and their importers in the other.
- `go vet` is a whole check, as before: hushrun knows no unit of it.

Three gaps, each seen in real runs:

1. **Coverage dropped.** With `-coverprofile=c.out`, a package hushrun reused wrote nothing to the
   profile: 8 lines became 7, one package gone, and the next step that uploads it would read a
   drop. `-cover` alone prints no `coverage:` for it either. The Jest and Vitest adapters already
   refuse to leave anything out while coverage is on (C-44) for that reason; Go didn't.
2. **A file no test binary is built from reran everything.** In a check `go test ./cart/
   ./tagged/`, editing `price/price.go`, a source `cart` imports, reran `cart` alone; editing
   `price/price_test.go`, which no binary of the check is built from, reran every package: the file
   isn't one `go list` names, so it was in the rest set. So were an unimported package's sources,
   a file the tags leave out, and in a `go.work` member's check (`cd app && go test ./...`) every
   file of the other module. The cost was inverted: the file that can't change the tests cost more
   than the one that does.
3. **`runtime` never left the machine below Go 1.27.** B-42 lets an assembler's object travel only
   when its header says `ObjFlagStd`, the standard library's. Go's assembler sets that only from
   Go 1.27, when it started taking `-std`; Go 1.24 to 1.26, all of which B-39 serves, never set it.
   On Go 1.24.7, 1.25.0 and 1.26.0 a small program's build kept 8 of its 24 packages here
   (`runtime`, `internal/bytealg` and every other standard package with assembly: `8 assembly
   outside the standard library`), and three of `build_go`'s tests failed; CI pins 1.27.4, so it
   never saw it. Each machine then compiled `runtime` itself.

## Decision

1. **Coverage on: every package runs, and nothing is left out** (C-71). A COMMAND with `-cover`
   (not `-cover=false`), `-coverprofile`, `-coverpkg` or `-covermode`, or `-test.coverprofile` or
   `-test.gocoverdir` after `-args`, or whose `go env GOFLAGS` holds one of the first four, makes no
   plan: `hushrun: <NAME>: every package runs: go test's coverage is on, and a partial run's
   coverage would read as a drop`, Jest's words. Each package still runs through hushrun's runner
   and is observed, so the map stays warm for a check without coverage.
2. **Every `.go` file of the scope is a unit or a source** (C-70). For Go, the sources set is no
   longer the `.go` files `go list` named, but every `.go` file that isn't a tested package's test
   file. A file in it that no package's tests read changes nothing that runs; one they read, as Go
   compiled it or as a test opened it, reruns those packages.

   Why this is safe: Go compiles a `.go` file only into its own directory's package, and a
   `_test.go` file only into that package's own test. Before every run the plan asks `go list` with
   COMMAND's flags and tags, and each package's key is made from what its test binary is built
   from now (`shared::lists`' `compiled`) with what its tests opened last time: a file that starts
   being compiled (its tag removed, or a package that starts importing it) is in that package's key,
   which then differs from the one recorded. A file added or deleted changes the sources' shape,
   which is in every key, so every package runs, as before. A test that opens one, its test log
   names. A package hushrun can't follow (`TestMain`, the repository's cgo, a program started) runs
   every time anyway.

   A file of another kind (`.s`, `.c`, `.h`, `go.mod`, `go.work`, data) stays where it was: the rest
   set unless a rule names it.
3. **The standard library's assembly is known by its package's Go object too** (B-150). An
   assembler's object counts as the standard library's when its own header says so, or when the
   archive's `_go_.o` says `ObjFlagStd` (and isn't assembly). The compiler sets that flag under
   `-std`, which `cmd/go` adds to a compile exactly when the package is standard, in every version
   B-39 names; `-std` given through `-gcflags` is already B-42's `altered`. What made standard
   assembly safe to share holds unchanged: its includes are the toolchain's, which B-43's identity
   hashes. A downloaded archive could claim either flag, as it could claim the assembler's before;
   that stays the audit's and the trust policy's to catch, as for every block.

4. **A Go fetch waits for the `open`s it needs** (B-45). On Vue's PR, the macOS job of
   `build_go`'s two-machine test reused 28 of 29 packages and built one, on code with no Go
   change. The cache program starts with COMMAND, while the check's `open` and its `go/1` `open`
   are still on the wire; a fetch that came then found the relay `Opening` or the manifest
   `Asking`, and was a miss at once. B-45 already promised a fetch waits for the manifest's lookup,
   at most a second or half its local cost; it now waits for the `open`s too, within the same
   bound, shared with the lookup. A test whose fake server answers `open` 400 ms late fails
   without it (`None` for a block the manifest names) and passes with it. Gradle's fetch goes
   through the same function and gets the same wait.

## Consequences

- `go test -coverprofile` through hushrun writes the profile a plain run would, every time; a
  check that wants reuse runs without coverage, under its own name.
- Editing a test of a package the check doesn't test, a package nothing imports, a file the tags
  leave out, or another module of a `go.work`, reruns only the packages that read it: usually none
  but the audit's sample.
- The first run of each check after this change reruns every package once: files that were in the
  rest set are sources now, which changes the rest set's digest and the sources' shape in every key.
  A machine that kept an older key simply doesn't match it.
- `hushrun why` decides by the same function, so it agrees with the run.
- Two hazards join the ones that must never miss a failure (`go_hazards_miss_no_failure`): two
  files' build tags swapped with no file added or removed, which only the packages' keys can tell,
  and a test file the tags left out whose tag is removed.
- With Go 1.24 to 1.26, `runtime` and the standard library's other assembly packages are shared
  as with 1.27. `build_go`'s tests pass on 1.24.7, 1.25.0, 1.26.0 and 1.27.4 here; CI runs 1.27.4
  only, so the older versions are proved by the unit test of B-150 and by this run, not by CI.
- A `-race` or `-cover` compile is another action id, Go's own, and is shared or kept by ADR
  0093's rules as any compile is.

## Alternatives considered

- **Sources: the `.go` files of directories with no listed package, only.** Narrower, and it left
  the common case, a test file of a package the check depends on and doesn't test, in the rest set.
- **Leaving coverage on and reusing anyway**, as the summary already leaves coverage out of a run
  that reused (C-34): the summary is right, but the profile a later step uploads isn't.
- **Merging the last coverage of reused packages into the profile**: hushrun would write a report
  of a run that didn't happen, and keep reports it says it keeps none of.
- **Sharing cgo packages' archives** (the C side of the audit): their id doesn't hold the C
  compiler's headers, ADR 0093's six proved cases of one id for other bytes include it, and keying
  them by what each C compile read needs a launcher Go has no hook for. Left as ADR 0093 decided.
