# 0070. `go test`'s packages, and what `hushrun ready` says of test files

- **Status:** accepted
- **Date:** 2026-11-11
- **Rules:** C-22, C-30, C-32, C-33, C-34, C-35; relies on C-14, C-31, ADR 0044, 0060, 0061, 0067,
  0068, 0069

## Context

ADR 0068 and 0069 observe and reuse test files with pytest (observed) and Cargo (compiler-given).
RFC 0003's milestone 4 adds more runners and has `hushrun ready` say how many test files CI would
run. This decision is Go's adapter, the second compiler-given one, the line `ready` adds, and three
things milestone 3 left: a map shared by two repositories, a suspension that a failing server
dropped, and pytest-xdist where the plan deselects every test left.

## Decision

1. **A unit is a package's tests** (C-30, C-35), named by the package's directory from the
   repository root, `.` for the root. Its files are its `_test.go` files, internal and external
   (`TestGoFiles`, `XTestGoFiles`): the unit rule is "the test files of the packages `go test`
   tests", as `go list -deps -test` names them, and `classify` stays the one function, with those
   files as units. In what people read a package is a test file, as a Rust test binary is.
2. **Reads** are what `go list -e -deps -test -json` says the package's test binary
   (`<package>.test`) is built from: every file it names (Go, cgo, assembly, embedded, test files)
   of every package in its `Deps` whose directory is in the repository, test variants included,
   and vendored packages and other modules of the repository with them. **Sources** are the `.go`
   files (`GoFiles`, `CgoFiles`, and the test variants' `GoFiles`) of the packages the list names,
   ADR 0068's rule for Rust: the files the toolchain names, of the kind it compiles. Everything
   else is in the rest set: `go.mod`, `go.sum`, `go.work`, `testdata/`, embedded files, C and
   assembly files, a file a build tag leaves out (it is named by no build), a package's test files
   when COMMAND doesn't test it.
3. **Go's own test log, as `go test` asks for it.** hushrun's runner starts each test binary with
   `-test.testlogfile=<file>`, the flag `go test` passes itself when it caches a result, and reads
   what the tests opened (`open`), looked at (`stat`) and where they moved (`chdir`), during the
   tests. A file of the repository they opened is read, a source read as data included. A
   directory they listed is kept in the map, and its files, every one below it, are read: a new
   file there changes the key. An `open` of an executable file is a program started (Go's
   `os.StartProcess` logs one), wherever it is: the package is `not observed`.
4. **What Go's log can't show makes a package `not observed`** (C-32): a `TestMain` (it runs before
   the log starts; hushrun knows of one from the test main `go list` writes, which calls it); the
   repository's cgo, SWIG or `.syso` code in the binary's packages (native code opens what it
   likes); a package `go list` can't load whole; a binary that exited 0 and wrote no log. What
   stays unseen: what packages open, or start, while they initialize, before any test (`init`,
   package variables), and what native code outside the repository does. That is `tests.always`'s
   case, as a Rust test's run-time reads are (RFC 0003 §7), and audit runs are the net.
5. **Loading, with COMMAND byte for byte as written.** hushrun adds
   `'-exec=<hushrun> __hushrun-test-runner go <N> <runner>…'` to `GOFLAGS`, after what `go env
   GOFLAGS` says (the environment's and `go env -w`'s), so that it wins as a flag given twice
   does, chained to an `-exec` already there. Go splits `GOFLAGS` and `-exec`'s value as
   `cmd/internal/quoted` does; a path with a quote can't be carried, and hushrun loads nothing.
   The runner starts the binary in its package's directory with `GOFLAGS` as COMMAND's
   environment had it (`HUSHRUN_GOFLAGS` carries it) and no `HUSHRUN_UNITS`, passes stdio, the exit
   status and signals through, as for Cargo.
6. **Go's test cache.** With `-exec`, `go test` neither stores nor finds a result: cmd/go writes no
   test log then, and its cache key holds the exec command. So every package runs, or is reused by
   hushrun; a test of this checks that a second run prints no `(cached)`. A `(cached)` package would
   never reach the runner: it would have no start, be `not run`, and record nothing, never a pass
   it didn't have.
7. **Selection.** The runner doesn't start a binary whose directory `hushrun.json` lists: `go
   test` prints `ok` for it, and `go vet` and the build still run, so a reused package that no
   longer builds still fails. The plan's keys come from `go list` asked before COMMAND, with
   COMMAND's flags that choose files (`-tags`, `-race`, `-msan`, `-asan`, `-mod`, `-modfile`, `-C`)
   in its environment, and from what the map says the package opened last time (a hint, C-34):
   the first run reuses nothing it couldn't compute, and later ones reuse with no map at all when
   the tests open nothing of the repository's.
8. **Which COMMANDs.** `go test [flags] [packages] [-args …]`, with flags hushrun knows. Not
   offered, said once: `-c` (runs nothing), `-exec` (its own runner), `-fuzz` (writes `testdata`),
   `-overlay`, `-toolexec`, `-compiler`, `-buildmode`, `-linkshared` (another build), `.go` files
   as arguments (a package of their own), an unknown flag, and a `GOOS`/`GOARCH` other than the
   host's (Go runs those through `go_$GOOS_$GOARCH_exec`, which hushrun doesn't chain to). `-json`
   is fine. A wrapper (`gotestsum`, `make`) hides `go test`'s arguments: COMMAND is not `go test`,
   the Go adapter isn't chosen, and the default one finds no pytest and does nothing.
9. **Environment.** Build tags, `GOOS`, `GOARCH`, `CGO_ENABLED` and `GOFLAGS` choose files, so
   `go list`, asked in COMMAND's environment, names the files this build compiles, and a machine
   that compiles other files computes other keys. What the environment changes without changing
   files is the check's own key's limit (C-9): it holds no environment.
10. **`hushrun ready`** (C-22) adds, for a check with `tests: reuse` that CI would run again, how
    many of its test files a run would run: `unit: CI would run 12 of 340 test files; 328 already
    passed on the files they read`, or with results on this machine `unit: a run here would run …;
    328 already passed here on the files they read`, or `unit: every test file runs: reuse of its
    test files is suspended …`. Only where the plan can be computed from files here and one batch
    lookup: pytest and Vitest (ADR 0071), whose plans read files and the map; not Cargo nor Go,
    whose plans need a build or `go list`, which `ready` doesn't start. Only for the files on disk:
    `--worktree`, or HEAD with nothing uncommitted. The lookup is a server's batch lookup as CI's
    jobs count a pass (C-10), never sending what waits in the outbox, which reads as suspended; a
    repository whose CI reuses only CI's results says nothing, as its check is `ci`. It writes
    nothing (ADR 0061).
11. **The map's id holds the repository** (C-33), as the run names it (C-10, lowercase, `""` for
    none): two repositories with one check and one command on one machine kept one map, which said
    a misleading "why every test file runs".
12. **A server that fails didn't take a suspension** (C-34). A `POST {base}/v0/units/…` answered
    with a 5xx or 429 after L-4's retries is an error, as one that can't be reached is: a
    suspension stays in the outbox, and nothing is reused until it reaches the server. Before this
    it read as a refusal, and the suspension was dropped.

## Consequences

- A Go repository reuses package tests on the same rule as Rust's test binaries, with what tests
  open at run time followed, which Rust's adapter doesn't do.
- Observing a Go check turns Go's own test cache off for it: every package runs that hushrun
  doesn't reuse.
- A package with a `TestMain`, or whose tests start a process, is never reused.
- `HUSHRUN_GOFLAGS` is the runner's, between hushrun and itself.

## Alternatives considered

- **Parsing `go test -json`:** COMMAND would change, and a wrapper's output is its own.
- **Building first with `go test -c`:** another build, and a binary per package to identify; the
  file lists `go list` gives are exact for the build `go test` makes in the same environment.
- **Reading test files for `TestMain` and `init`:** hushrun reads no language; the test main Go
  writes says whether a `TestMain` is called, and package initialization stays a stated limit.
- **Offering Go's adapter in `ready`:** `go list` is a tool the repository chooses (its `go.mod`
  may switch toolchains), and `ready` starts none.
