# 0074. Test files on the dashboard, and milestone 4's loose ends

- **Status:** accepted
- **Date:** 2026-11-11
- **Rules:** C-10, C-22, C-34, C-35, L-14; relies on C-12, C-33, ADR 0068, 0069, 0070, 0071

## Context

RFC 0003 §3.11: "What the dashboard gains: test files run and reused per check, and the time
saved, beside the minutes saved by skips." A run's result already says `units: {total, ran,
reused}` (C-10) and goes wherever the result does; the time reusing saved was computed for the
run's last line and went nowhere. Milestone 4 also left three things to tidy.

## Decision

1. **`units.savedMs`** (C-10, results-v0 §3). A run that reused test files adds to its `units` how
   long they took when they last ran, by their recorded results: the number its last line already
   says (C-27). Never in `observe`, which saved nothing, nor when none was reused. It is additive:
   a server from before keeps the record as sent, and a CLI from before sends none.
2. **The server keeps the counts as numbers** (cloud): `run_results` gains `units_total`,
   `units_ran`, `units_reused` and `units_saved_ms`, all null without `units` (every skip), with a
   `CHECK` that ties them together (no counts on a skip; ran and reused add up to at most total;
   no saving without a reuse). A result's `units` is checked as it is recorded, as `reports` and
   `ranAnyway` are: one that doesn't read is refused (400), since the record is kept as signed. The
   migration fills the columns from records that already say `units`, where they read; a record
   from before `savedMs` has a null saving. The checks view adds, per check and in all,
   `test_file_runs`, `test_files_ran`, `test_files_reused` and `reuse_saved_ms`, and the dashboard
   shows them beside the time skips saved, only where a run said them. No test name or path is
   involved: these are counts.
3. **`hushrun repository stats`** (L-14) shows the same numbers for this machine's results when any
   run said them: `test files  12 ran, 328 reused, in 2 runs` and `saved by reuse`, and in JSON
   `testFiles: {runs, ran, reused, savedMs}`; without them, nothing new.
4. **pytest speaks only where it reported** (C-22, C-34). A check whose COMMAND hushrun doesn't
   recognize gets the pytest adapter, which does nothing where pytest never starts (`npm test`,
   `make test`). Its plan, and `hushrun ready`'s line, counted the Python files that look like tests
   all the same. Now one rule, `speaks_of_test_files`, says: for the pytest adapter, nothing about
   test files before a run until this machine's store holds the check, which happens only once
   pytest reported its test files here; an adapter recognized from COMMAND always speaks. A real
   pytest check's first run so says nothing before COMMAND, and from its second on, what it says
   today.
5. **A Go program started from another directory** (C-35). `os.StartProcess` logs the program as
   given, then looks at `exec.Cmd.Dir`, and the kernel finds a relative program from there, not
   from the test's directory; and the log is read after the tests, when a program they wrote to a
   temporary directory is gone. So a relative `open` followed at once by a `stat` of a directory
   from which it names an executable file, or of a directory that is gone, is a program started;
   so is an `open` of a file that is gone right after an `open` of `/dev/null`, which `exec.Cmd`
   opens for a child's standard streams just before it starts it. Its package is `not observed`. A
   hazard of Go's suite (a source read only by such a program) missed a failure before, and none
   now. What stays unseen: a program started with `SysProcAttr` and `Dir` from a directory that
   still exists, which no log line distinguishes; audit runs are under it, as under package init.
6. **A reused Go package stays in the map.** The map kept a test file while its file was in the
   scope; a Go package is named by its directory, which is no file, so a reused package was dropped
   and the next plan lost what it had opened. It is kept while a file is below its directory.

## Consequences

- The dashboard shows, per check and in all, how many test files runs reused and the time that
  saved; an org that never turned `tests` on sees nothing new.
- A non-Python check with `tests` on prints nothing about test files; a pytest check's first run
  prints no plan line.

## Alternatives considered

- **Reading `units` out of the stored record in the checks view's queries:** a record from another
  client may say anything, and a cast in an aggregate would fail the whole view; typed columns
  checked once, at write, can't.
- **Folding the reuse saving into "CI time saved":** a skip saved a whole run and a reuse part of one,
  measured differently; the RFC asks for them side by side.
- **Telling pytest checks by COMMAND (`pytest`, `python -m pytest`):** `make test` running pytest is
  a pytest check; that a pytest reported is what is known.
