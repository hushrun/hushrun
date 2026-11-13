# 0069. A check's test files, reused, audited and suspended

- **Status:** accepted
- **Date:** 2026-11-10
- **Rules:** C-5, C-10, C-12, C-26, C-27, C-32, C-33, C-34; relies on C-9, C-14, C-23, C-25, C-30,
  C-31, ADR 0032, 0044, 0047, 0048, 0060, 0067, 0068

## Context

ADR 0068 observes a check's test files: what each read, how it ended, and what reuse would have
done. This decision makes reuse real. Inside a run whose check missed, a test file whose newest
result that counts here passed on the files it reads now isn't run again. A wrong reuse is the
mistake a wrong skip is (ADR 0044), so every rule here chooses a run over a guess, and every failure
of the machinery ends in "every test file runs".

## Decision

1. **The plan comes before COMMAND** (C-34). For pytest, each test file's key is computed from what
   the check's map says it read last time: a hint, never trusted, since a wrong list makes a key no
   run recorded. For Cargo, hushrun makes COMMAND's build first (`cargo test --no-run` with
   COMMAND's build options, its progress on stderr), so COMMAND's own finds it done, and takes the
   keys from that build's dep-info. One batch lookup asks for the newest result that counts here
   for every key, with the check's reuse state. A test file runs when the map doesn't have it,
   `tests.always` names it, it wasn't fully observed last time, its newest result is a fail or
   none, or the audit chose it.
2. **Adapters deselect what `hushrun.json` lists, and say exactly what** (C-32). COMMAND stays
   byte for byte as written. The pytest plugin removes, after collection and every other
   deselection, the tests of the listed test files, and tells pytest they were deselected. Only
   one pytest may: the first that starts with the run's directory claims it, and a pytest-xdist
   worker may when that one started it; a nested pytest, or a second pytest of the same COMMAND,
   deselects nothing. One whose `python_files` isn't what hushrun read deselects nothing. The
   target runner doesn't start a listed test binary, recognized by its path and the SHA-256 of
   its bytes: nextest's `--list` of it lists no test, and `cargo test`'s start of it exits 0.
3. **A test file is reused only when the plan listed it and the adapter deselected it**, by the
   same state function as before (C-32), which now puts a failure first: a process the plan
   doesn't speak for may run a test file another deselected, and its failure must never read as a
   reuse. Across processes, a test file is deselected only when every process that saw it
   deselected it. Reuse never chains: a reused test file records nothing.
4. **A check that passed with reused test files is a pass** (C-10, RFC 0003 D2): the same files
   skip it whole next time. `units` says `{total, ran, reused}`, and `ranAnyway` when something
   stopped all reuse.
5. **pytest's "no tests collected"** (exit 5) when every test left was deselected by the plugin
   would turn a pass into a fail, so the plugin's process exits 0 then, and only then: a failure or
   an error gives pytest another status first, and with nothing deselected by it 5 stays 5. With
   pytest-xdist, the process that claimed the directory counts what its workers deselected.
6. **Audit runs** (C-34, RFC 0003 D5). 2% of the test files that could be reused, rounded up and
   at least one, run anyway, chosen by the SHA-256 of each one's key and the run's start, so nobody
   picks. One that passes agrees. One that fails, and one the plan listed that ran all the same and
   failed, runs once more after COMMAND: COMMAND as written, again, with a directory whose
   `hushrun.json` lists every other test file the first run saw, so that only those run, for both
   adapters. Passes then: it may be flaky, and is said so, as G13 says of a check; the first run's
   fail is what goes under its key, so it runs next time, as a check that failed does. Anything
   else, a second run that couldn't be set up or ended without a verdict included: a
   disagreement. The run keeps COMMAND's exit code, the disagreement is kept, and the check's reuse
   is suspended for the repository.
7. **The reuse state has one home** (C-34): `on` or `suspended`, with who and when, and a
   generation that counts its transitions. With a server, the server's record of the check in the
   repository, read with the batch lookup and written with the guard in the statement. In local
   mode, this machine's file, read and written whole under a lock with the same transitions. A
   suspension that can't reach the server waits in this machine's outbox, which the next plan
   sends before it reads anything; while it waits, nothing is reused. Suspending only ever makes
   test files run, so anyone who records results may; a suspended check keeps its first
   suspension.
8. **People turn it on again**: `hushrun tests resume NAME` reads the state, says who suspended it
   and when, and turns it on naming the generation it read. A check suspended again since, or
   turned on by someone else, is left, and said. A coding agent can't, by C-23's rule of who is an
   agent, and the server takes a personal token only, reading the person's membership again under
   the row's lock.
9. **Reports** (C-26, C-27). In CI where a later step reads the check's reports, no test file is
   reused: a reused one writes none. A summary of a run with reused test files leaves coverage out,
   since a partial figure would read as a drop; its tests count what ran.
10. **What people see.** Before COMMAND, `hushrun: unit: 12 of 340 test files run; 328 already
    passed on the files they read, and 7 more that already passed run anyway, as a check`; `all 340 test files run:
    uv.lock changed, and any test may read it`; or `every test file runs: <why>`. The last line
    adds `; reusing 328 test files saved about 3m 05s`, from the reused test files' recorded
    durations.
11. **The protocol** (C-12, results-v0 §3). `POST {base}/v0/runs/{key}` records a run's result with
    its test files' results in one request, signed as a result is; `POST {base}/v0/units/lookup`
    answers up to 5,000 keys with each one's newest result whose run counts for the caller, with
    that run's record so that the CLI decides again, and the reuse state; `POST
    {base}/v0/units/reuse` suspends and resumes. Keys are hashes: no test name or path goes. A run
    with reused test files is one check, and its test files aren't metered (D7), 5,000 at most a
    run. A server from before these (404, 405) records the run alone, and nothing is reused: said
    once.
12. **Fail open.** A lookup that fails, a server that refuses, a plan that can't be written, a
    build that fails, more than 5,000 test files: every test file runs. `observe` is unchanged;
    `tests` off changes nothing.

## Consequences

- A check's own pass still decides first: reuse only shortens a run that would have run whole.
- Every run in `reuse` runs at least one test file that could have been reused: a check with one
  test file never reuses it.
- A test file that only passes after another ran (shared state) can fail when that one is reused;
  its second run fails too, and the check's reuse is suspended. That is right: reuse isn't safe
  for it.
- Ctrl-C during the second run, or anything else that leaves it without a verdict, suspends reuse.
  A person turns it on again.
- With pytest-xdist, if the claiming process can't read what its workers deselected, an all-
  deselected run exits 5, and the check fails: a run of the same files without reuse passes.

## Alternatives considered

- **Not importing reused test files (`pytest_ignore_collect`):** faster, but then hushrun can't tell
  whether the files it skipped held any selected test, so exit 5 couldn't be turned into a pass
  soundly, and a reused file whose import now fails would go unseen.
- **Rewriting exit 5 in hushrun:** COMMAND may be a script that runs pytest and more, which stops at
  pytest's 5: hushrun would claim what never ran.
- **Recording the second run's pass under the key:** two results for one key in one run, and the
  next run would reuse a test file that just failed once; the first fail runs it again instead.
- **Suspension with the run's record:** a record the org's plan refuses (402) would lose it; a
  request of its own, never metered, with an outbox, doesn't.
- **The reuse state by check id (with COMMAND):** `pytest -n 2` would dodge `pytest`'s suspension.
  By the check's name, every command of it is suspended.
- **Only reporting a disagreement (D5's other option):** a known-wrong reuse would go on.
