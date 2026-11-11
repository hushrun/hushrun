# 0073. `hushrun why`: what a run would do with one test file, and why

- **Status:** accepted
- **Date:** 2026-11-11
- **Rules:** C-37; relies on C-11, C-22, C-30 to C-36, ADR 0048, 0061, 0067, 0072

## Context

With `tests: reuse`, a run says how many test files it runs and reuses (C-34), and when every test
file runs, one reason. It doesn't say why one particular test file runs: the question a person asks
when a change to one module runs a test file they didn't expect, or doesn't run one they did. RFC
0003 §1.3 asked for:

```
$ hushrun why unit tests/test_cart.py
tests/test_cart.py runs: it read shop/pricing.py, which changed
  tests/test_cart.py imports shop/cart.py imports shop/pricing.py
```

The plan decides from a key and a recorded result; an explanation must never become a second
decision that can disagree with it, and must not change what it explains.

## Decision

1. **`hushrun why NAME UNIT [-- COMMAND]`** (C-37), for the files on disk now. UNIT is from the
   current directory or the root; for Go, a package's directory or one of its test files. COMMAND,
   cwd and scope are the check's newest run here, as the store keeps them (ADR 0072); with several
   commands for one name it takes the newest and says so; `-- COMMAND` (and `--scope`) names
   another. It needs no `hushrun.yaml` beyond what a run needs, writes nothing (ADR 0061), and
   agents may run it.
2. **One answer, in the order the plan uses**, each one sentence:
   - `tests` off, or the adapter not offered for COMMAND (why, as the run would say it);
   - not a test file of the check: outside the scope, in `tests.ignore`, or not named by the unit
     rule (pytest's `python_files`, Vitest's list, a test binary's root file, a tested package);
   - `tests.always` names it;
   - this machine has never seen it run;
   - not observed last time, with what couldn't be followed (a process it started, and which);
   - suspended, by whom and when, and how a person turns it on again;
   - would be reused: it passed on the files it read and none has changed, and an audit run may
     still run it;
   - its newest result on these files failed;
   - with no result under its key: a file it read changed or is gone (named, with the path of edges
     the adapter saw when there is one, and for Cargo what the test binary is built from), a file is
     new below a directory it listed, a file of the rest set changed, is new or gone, or a source
     is new or gone (every test file runs); else no pass that counts here is recorded for what it
     read, unchanged since it last ran here.

   In `observe` it says what `tests: reuse` would do.
3. **What needs the lookup, and what doesn't.** The reasons up to "never seen" and "not observed"
   come from `hushrun.yaml`, COMMAND and the store alone: they make a test file run whatever any
   result says. Suspended, reused and failed need the results: one read-only batch lookup, as
   `hushrun ready`'s (C-22), of this machine's results or the server's, asked as a run here counts
   a pass (its CI job when it runs in one), sending nothing that waits in the outbox, which reads as
   suspended. A key is computed from the map with the plan's own function; nothing new decides.
4. **"As far as this machine knows"** is said, in the text and as `asFarAsThisMachineKnows` in
   JSON, when the lookup can't answer (results off, a server out of reach) and the answer falls back
   to what changed, and for Cargo, Go and Vitest, whose test files and what each is built from are
   known from the last build or run here: `hushrun why` starts no build, `go list` or Vitest.
5. **Edges** (ADR 0072): pytest writes, beside its test files' facts, the direct edges it saw (a
   file imports or opens another, a test file reads another through no edge of its own); Vitest the
   imports of each project's Vite module graph; Go each package's imports (`go list`'s `Imports`) and
   what each package's tests opened. Cargo has none: the compiler's dep-info says what a binary is
   built from, not through what. A path is the shortest through files the test file read, imports
   and opens first. Edges are never used for a key, and an adapter that can't report them reports
   none, which costs only the path.
6. **`--format json`**, `hushrun.why/v1`: `check`, `command`, `unit`, `decision` (`runs`, `reused`,
   `not-a-test-file`, `off`), `reason` (a word), `says`, `detail`, `asFarAsThisMachineKnows`,
   `file`, `path` (`[{file, then}]`). It exits 0 whenever it explains, 2 outside a repository.

## Consequences

- A person can ask about one test file and get the plan's own answer, with the files between it and
  what changed.
- What `why` says about Cargo, Go and Vitest can be stale after a build or a run that changed
  which test files exist; it says so.
- No test name, test path or source path goes to a server: the lookup sends keys only (ADR 0048).

## Alternatives considered

- **A graph view:** RFC 0003 §1.3 puts one here; text with a path answers the question, and a view
  can read `hushrun.why/v1` later.
- **Running `go list` or a build in `why`:** exact for those adapters, but `why` would start the
  repository's tools, as `ready` doesn't (ADR 0070), and take as long as a build.
- **Answering every reason from the store alone:** a pass recorded on another machine, or a
  failure, would read wrong; the lookup is what the plan asks.
